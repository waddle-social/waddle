import Foundation
import WaddleKit

/// A moderation action on a room member.
enum MemberAction: Hashable, CaseIterable {
    case makeAdmin
    case makeMember
    case removeMember
    case ban
    case kick

    /// The XEP-0045 affiliation the action sets; nil for a kick (role).
    var affiliation: RoomAffiliation? {
        switch self {
        case .makeAdmin: return .admin
        case .makeMember: return .member
        case .removeMember: return RoomAffiliation.none
        case .ban: return .outcast
        case .kick: return nil
        }
    }

    var title: String {
        switch self {
        case .makeAdmin: return "Make admin"
        case .makeMember: return "Make member"
        case .removeMember: return "Remove member"
        case .ban: return "Ban"
        case .kick: return "Kick"
        }
    }

    var symbol: String {
        switch self {
        case .makeAdmin: return "shield"
        case .makeMember: return "person.badge.plus"
        case .removeMember: return "person.badge.minus"
        case .ban: return "nosign"
        case .kick: return "figure.walk.departure"
        }
    }

    var isDestructive: Bool {
        switch self {
        case .removeMember, .ban, .kick: return true
        case .makeAdmin, .makeMember: return false
        }
    }
}

/// Who a member row describes.
struct MemberSubject: Hashable {
    /// Occupant nick; for an absent member, its reserved nick if any.
    let nick: String?
    /// Real bare JID, when the room exposes it or the member list names it.
    let jid: BareJID?
    let affiliation: RoomAffiliation
    let isPresent: Bool
}

/// XEP-0045 permission rules for the member context menu. The server stays
/// authoritative; this only hides actions it would refuse.
enum MemberPermissions {
    static func actions(
        on subject: MemberSubject,
        by actor: Occupant?,
        account: AccountIdentity
    ) -> [MemberAction] {
        guard let actor, !isSelf(subject, account: account) else { return [] }
        return affiliationActions(on: subject, by: actor) + kickAction(on: subject, by: actor)
    }

    static func isSelf(_ subject: MemberSubject, account: AccountIdentity) -> Bool {
        if let jid = subject.jid, jid == account.jid { return true }
        return subject.isPresent && subject.nick == account.nick
    }

    /// §10 owner use cases: owners may grant or revoke any affiliation.
    /// §9 admin use cases: admins manage members and bans, and cannot touch
    /// owners or admins (only owners edit the admin list, §10.8).
    static func affiliationActions(on subject: MemberSubject, by actor: Occupant) -> [MemberAction] {
        guard subject.jid != nil else { return [] }
        let current = subject.affiliation
        switch actor.affiliation {
        case .owner:
            var actions: [MemberAction] = []
            if current != .admin { actions.append(.makeAdmin) }
            if current != .member { actions.append(.makeMember) }
            if current == .owner || current == .admin || current == .member { actions.append(.removeMember) }
            if current != .outcast { actions.append(.ban) }
            return actions
        case .admin:
            switch current {
            case .none: return [.makeMember, .ban]
            case .member: return [.removeMember, .ban]
            case .owner, .admin, .outcast: return []
            }
        case .member, .none, .outcast:
            return []
        }
    }

    /// §8.2: moderators kick present occupants whose affiliation is not
    /// above their own.
    static func kickAction(on subject: MemberSubject, by actor: Occupant) -> [MemberAction] {
        guard actor.role == .moderator, subject.isPresent, subject.nick != nil else { return [] }
        return rank(actor.affiliation) >= rank(subject.affiliation) ? [.kick] : []
    }

    static func rank(_ affiliation: RoomAffiliation) -> Int {
        switch affiliation {
        case .owner: return 3
        case .admin: return 2
        case .member: return 1
        case .none: return 0
        case .outcast: return -1
        }
    }
}

/// Role and affiliation copy.
enum MemberLabels {
    static func groupTitle(_ role: RoomRole) -> String {
        switch role {
        case .moderator: return "Moderators"
        case .participant: return "Participants"
        case .visitor: return "Visitors"
        case .none: return "Others"
        }
    }

    /// Badge text; nil for users without an affiliation.
    static func badge(_ affiliation: RoomAffiliation) -> String? {
        switch affiliation {
        case .owner: return "Owner"
        case .admin: return "Admin"
        case .member: return "Member"
        case .outcast: return "Banned"
        case .none: return nil
        }
    }
}
