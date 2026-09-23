import Foundation
import WaddleKit

/// Occupants that share a XEP-0045 role.
struct MemberRoleGroup: Identifiable, Hashable {
    let role: RoomRole
    let occupants: [Occupant]

    var id: RoomRole { role }
}

/// Builds the member list of the details screen.
enum MemberRoster {
    static let roleOrder: [RoomRole] = [.moderator, .participant, .visitor]

    /// Present occupants grouped Moderators, Participants, Visitors; empty
    /// groups dropped; each sorted by nick.
    static func grouped(_ occupants: [Occupant]) -> [MemberRoleGroup] {
        roleOrder.compactMap { role in
            let members = occupants.filter { $0.role == role }.sorted(by: nickOrder)
            return members.isEmpty ? nil : MemberRoleGroup(role: role, occupants: members)
        }
    }

    /// XEP-0045 affiliated users not currently in the room. A member counts
    /// as present when an occupant exposes the same real JID or, in a
    /// semi-anonymous room, uses the member's reserved nick. Outcasts are
    /// not members.
    static func absent(_ members: [RoomMember], present occupants: [Occupant]) -> [RoomMember] {
        let presentJIDs = Set(occupants.compactMap(\.realJID))
        let presentNicks = Set(occupants.map { $0.nick.lowercased() })
        return members
            .filter { $0.affiliation != .outcast }
            .filter { !presentJIDs.contains($0.jid) }
            .filter { member in member.nick.map { !presentNicks.contains($0.lowercased()) } ?? true }
            .sorted(by: memberOrder)
    }

    static func displayName(of member: RoomMember) -> String {
        if let nick = member.nick?.trimmingCharacters(in: .whitespacesAndNewlines), !nick.isEmpty {
            return nick
        }
        return member.jid.localpart ?? member.jid.domain
    }

    /// Applies a successful affiliation change to a loaded member list.
    static func updating(_ members: [RoomMember], jid: BareJID, to affiliation: RoomAffiliation) -> [RoomMember] {
        guard affiliation != .none else { return members.filter { $0.jid != jid } }
        guard let index = members.firstIndex(where: { $0.jid == jid }) else {
            return members + [RoomMember(jid: jid, nick: nil, affiliation: affiliation)]
        }
        var updated = members
        let existing = members[index]
        updated[index] = RoomMember(jid: existing.jid, nick: existing.nick, affiliation: affiliation)
        return updated
    }

    private static func nickOrder(_ lhs: Occupant, _ rhs: Occupant) -> Bool {
        lhs.nick.localizedCaseInsensitiveCompare(rhs.nick) == .orderedAscending
    }

    private static func memberOrder(_ lhs: RoomMember, _ rhs: RoomMember) -> Bool {
        if lhs.affiliation != rhs.affiliation { return lhs.affiliation < rhs.affiliation }
        return displayName(of: lhs).localizedCaseInsensitiveCompare(displayName(of: rhs)) == .orderedAscending
    }
}
